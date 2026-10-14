import {expect,test} from './_test';
import {installSmokeMocks} from './_smokeMocks';
import {mountPage} from './_helpers';
test('a recorded child location resolves its name under the native root-only list contract',async({page})=>{
 await installSmokeMocks(page);
 await page.route(/\/api\/people$/,r=>r.fulfill({json:[{id:'emp-child-review',name:'Child Location Person',email:'child@test',role:'platform-admin',department:'it',status:'active',location:'loc-child',hire_date:'2020-01-01',skills:[],certifications:[]}]}));
 await page.route(/\/api\/locations$/,r=>r.fulfill({json:[{id:'loc-root',name:'Root Place',parent_id:null}]}));
 await page.route(/\/api\/locations\?parent_id=loc-root$/,r=>r.fulfill({json:[{id:'loc-child',name:'Recorded Child Place',parent_id:'loc-root'}]}));
 await page.route(/\/api\/locations\/loc-child$/,r=>r.fulfill({json:{id:'loc-child',name:'Recorded Child Place',parent_id:'loc-root'}}));
 await mountPage(page,'/ux/people');
 await expect(page.locator('tbody tr').filter({hasText:'emp-child-review'}).locator('td').nth(4)).toHaveText('Recorded Child Place');
});

for (const status of [404, 403, 503]) {
 test(`location ${status} remains distinct from unspecified`, async ({page}) => {
  await installSmokeMocks(page);
  const person = (id: string, location: string | null) => ({id,name:id,email:'test@test',role:'platform-admin',department:'it',status:'active',location,hire_date:'2020-01-01',skills:[],certifications:[]});
  await page.route(/\/api\/people$/,r=>r.fulfill({json:[person('emp-one','loc-child'),person('emp-two','loc-child'),person('emp-none',null)]}));
  let calls=0;
  await page.route(/\/api\/locations\/loc-child$/,r=>{calls++;return r.fulfill({status,json:'unavailable'});});
  await mountPage(page,'/ux/people');
  const label=status===404?'Unresolved location — loc-child':'Location unavailable — loc-child';
  await expect(page.locator('tbody tr').filter({hasText:'emp-one'}).locator('td').nth(4)).toHaveText(label);
  await expect(page.locator('tbody tr').filter({hasText:'emp-two'}).locator('td').nth(4)).toHaveText(label);
  await expect(page.locator('tbody tr').filter({hasText:'emp-none'}).locator('td').nth(4)).toHaveText('Unspecified');
  expect(calls).toBe(1);
 });
}

test('a late response cannot replace a remounted location identity read',async({page})=>{
 await installSmokeMocks(page);
 const employee={id:'emp-review',name:'Review Person',email:'review@test',role:'platform-admin',department:'it',status:'active',location:'loc-review',hire_date:'2020-01-01',skills:['Stored skill'],certifications:[]};
 await page.route(/\/api\/people$/,r=>r.fulfill({json:[employee]}));
 await page.route(/\/api\/people\/emp-review$/,r=>r.fulfill({json:employee}));
 let release=()=>{};let finished=()=>{};let reads=0;
 const held=new Promise<void>(r=>{release=r});const ended=new Promise<void>(r=>{finished=r});
 await page.route(/\/api\/locations\/loc-review$/,async r=>{
  reads++;
  if(reads===1){await held;await r.fulfill({json:{id:'loc-review',name:'Stale first name'}});finished();}
  else await r.fulfill({json:{id:'loc-review',name:'Fresh second name'}});
 });
 await mountPage(page,'/ux/people');
 await expect(page.locator('tbody tr').filter({hasText:'emp-review'})).toContainText('Loading location — loc-review');
 await page.getByRole('link',{name:'emp-review',exact:true}).click();
 await expect(page).toHaveURL(/\/ux\/people\/emp-review$/);
 await page.goBack();
 await expect(page.locator('tbody tr').filter({hasText:'emp-review'})).toContainText('Fresh second name');
 release();await ended;
 await expect(page.locator('tbody tr').filter({hasText:'emp-review'})).toContainText('Fresh second name');
 await expect(page.locator('tbody tr')).not.toContainText('Stale first name');
});
